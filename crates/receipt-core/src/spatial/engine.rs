//! Spatial item extraction: pair each description with the price printed in the
//! amount column beside it, using the word boxes rather than the line text.

use super::candidate::{select_spatial_item_line, SpatialLineCandidate};
use super::patterns::*;
use super::types::*;
use crate::common::ReceiptWarningKind;
use crate::money::Money;
use crate::ocr_document::{OcrDocument, OcrLine, OcrWord};

fn normalize_decimal_spacing(text: &str) -> String {
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

fn parse_scaled_decimal(token: &str) -> Option<i64> {
    let trimmed = token.trim();
    let (whole, frac) = trimmed.split_once('.')?;
    if whole.is_empty() || frac.len() != 2 {
        return None;
    }
    if !whole.chars().all(|ch| ch.is_ascii_digit()) || !frac.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let whole_value = whole.parse::<i64>().ok()?;
    let frac_value = frac.parse::<i64>().ok()?;
    Some(whole_value * SCALE + frac_value * 100)
}

fn alpha_ratio(value: &str) -> f64 {
    let non_ws_count = value.chars().filter(|ch| !ch.is_whitespace()).count();
    if non_ws_count == 0 {
        return 0.0;
    }
    let alpha_count = value.chars().filter(|ch| ch.is_ascii_alphabetic()).count();
    alpha_count as f64 / non_ws_count as f64
}

fn is_section_name(text: &str) -> bool {
    matches!(
        text,
        "MEAT" | "SEAFOOD" | "PRODUCE" | "DELI" | "GROCERY" | "BAKERY" | "FROZEN" | "FOOD"
    )
}

fn strip_leading_receipt_codes(text: &str) -> String {
    let trimmed = text.trim();
    let trimmed = re_leading_qty_prefix().replace(trimmed, "");
    let trimmed = re_leading_long_sku().replace(trimmed.as_ref(), "");
    let trimmed = re_leading_short_code().replace(trimmed.as_ref(), "$rest");
    let trimmed = re_leading_section_item_prefix().replace(trimmed.as_ref(), "");
    trimmed.trim().to_string()
}

fn is_section_header_text(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    let normalized = re_multi_spaces()
        .replace(&text.trim().to_ascii_uppercase(), " ")
        .to_string();
    if re_dept_marker_prefix().is_match(&normalized) {
        return true;
    }
    if is_section_name(normalized.as_str()) {
        return true;
    }
    if re_section_header_with_aisle().is_match(&normalized) {
        return true;
    }
    if re_section_aisle_prefix().is_match(&normalized) {
        let remainder = re_section_aisle_prefix()
            .replace(&normalized, "")
            .trim()
            .to_string();
        let words = re_ascii_words()
            .find_iter(&remainder)
            .map(|m| m.as_str())
            .collect::<Vec<_>>();
        if words.len() == 1 && is_section_name(words[0]) {
            return true;
        }
    }
    false
}

fn is_summary_line(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    let upper = text.trim().to_ascii_uppercase();
    // "Member Pricing" / "Manager's Special" rows on Loblaws-family receipts
    // are line-item discounts (negative price), not membership/store-info
    // metadata, so they must NOT match the `^MEMBER\b` arm of
    // re_summary_patterns. Without this carve-out the discount line is
    // filtered, the negative price is dropped, and the items sum overshoots
    // the printed subtotal (RCSS rcss_20260130 drops -$1.49 and -$0.98).
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

fn trailing_price_scaled(text: &str) -> Option<i64> {
    let normalized = normalize_decimal_spacing(text.trim());
    let captures = re_trailing_price().captures(&normalized)?;
    let value = parse_scaled_decimal(captures.get(1)?.as_str())?;
    let is_negative = captures.get(2).map(|m| m.as_str() == "-").unwrap_or(false);
    Some(if is_negative { -value } else { value })
}

fn line_has_trailing_price(text: &str) -> bool {
    trailing_price_scaled(text).is_some()
}

fn looks_like_onsale_marker(text: &str) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    let normalized = normalize_decimal_spacing(&text.trim().to_ascii_uppercase());
    let without_price = re_trailing_price().replace(&normalized, "").to_string();
    let compact: String = without_price
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect();
    if compact.ends_with("ONSALE") || compact.ends_with("ONSAL") {
        let prefix_len = compact.len().saturating_sub(6);
        return prefix_len <= 3;
    }
    false
}

fn is_priced_generic_item_label(left_text: &str, full_text: &str) -> bool {
    if left_text.trim().is_empty() {
        return false;
    }
    line_has_trailing_price(full_text)
        && matches!(
            left_text.trim().to_ascii_uppercase().as_str(),
            "MEAT" | "BAKERY"
        )
}

fn parse_quantity_modifier(text: &str) -> bool {
    re_count_at_price().is_match(text)
        || re_weight_at_price().is_match(text)
        || re_multi_for_price().is_match(text)
}

fn looks_like_quantity_expression(text: &str) -> bool {
    let normalized = normalize_decimal_spacing(text.trim());
    if normalized.is_empty() {
        return false;
    }
    if parse_quantity_modifier(&normalized) {
        return true;
    }
    let upper = normalized.to_ascii_uppercase();
    if upper.starts_with('(') && upper.contains('@') && upper.contains("/$") {
        let alpha_count = upper.chars().filter(|ch| ch.is_ascii_alphabetic()).count();
        if alpha_count <= 2 {
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
    re_multi_for_price().is_match(&normalized)
        || re_compact_offer_fragment().is_match(&normalized)
        || re_parenthetical_offer_prefix().is_match(&normalized)
}

/// True when a line looks like the store's address / branch header.
///
/// The pattern alternates over bare tokens — `RD`, `ST`, `DR`, `ON` — so it
/// fires on any item whose description happens to contain one: `ON THE GO
/// BOTTLE`, `TNS RD HT BF BRR`, `5 ON CITRUS`, `YIN ON SWEETENED SOYA DRINK`.
/// Widening the token list is not the answer; `ON` and `DR` are ordinary
/// English (`DR PEPPER`), and no spelling of the alternation fixes that.
///
/// What separates the two cases is not the words but the **price**: the store
/// address is printed in the header/footer and never carries one, while an item
/// row that owns a trailing price is an item by construction. So the address
/// veto only applies to unpriced lines — see [`is_valid_item_line`], the sole
/// caller. Priced lines still face every other guard there (summary, section
/// header, metadata, alpha-ratio), which is what keeps `Tota1$ On Promotion
/// Item(*)16.93` out.
pub(crate) fn footer_address_like(text: &str) -> bool {
    if line_has_trailing_price(text) {
        return false;
    }
    re_footer_address_patterns().is_match(&text.to_ascii_uppercase())
}

fn receipt_metadata_like(text: &str) -> bool {
    re_receipt_metadata_patterns().is_match(text.trim())
}

fn clean_description(desc: &str) -> String {
    let mut cleaned = desc.to_string();
    cleaned = re_leading_qty_prefix().replace(&cleaned, "").to_string();
    cleaned = re_sale_marker().replace_all(&cleaned, "").to_string();
    cleaned = re_hed_marker().replace_all(&cleaned, "").to_string();
    cleaned = re_hhed_marker().replace_all(&cleaned, "").to_string();
    cleaned = re_qty_price_marker().replace_all(&cleaned, "").to_string();
    cleaned = re_qty_price_marker_2()
        .replace_all(&cleaned, "")
        .to_string();
    cleaned = re_unit_price_marker().replace_all(&cleaned, "").to_string();
    cleaned = re_inline_price().replace_all(&cleaned, "").to_string();
    cleaned = re_garbled_price_artifact()
        .replace_all(&cleaned, "")
        .to_string();
    cleaned = re_leading_section_item_prefix()
        .replace(&cleaned, "")
        .to_string();
    cleaned = re_cahrd().replace_all(&cleaned, "").to_string();
    cleaned = re_hed_word().replace_all(&cleaned, "").to_string();
    cleaned = re_leading_non_alnum().replace(&cleaned, "").to_string();
    cleaned = re_trailing_non_alnum().replace(&cleaned, "").to_string();
    cleaned = re_multi_spaces().replace_all(&cleaned, " ").to_string();
    cleaned.trim().to_string()
}

fn is_deposit_stub(text: &str) -> bool {
    let cleaned = clean_description(text);
    let upper = cleaned.to_ascii_uppercase();
    upper == "DEPOSIT" || upper.starts_with("DEPOSIT ")
}

fn lacks_description_context(text: &str) -> bool {
    let stripped = strip_leading_receipt_codes(text);
    stripped.is_empty() || alpha_ratio(&stripped) < 0.5
}

pub(crate) fn is_price_word(text: &str) -> Option<i64> {
    let normalized = normalize_decimal_spacing(text.trim());
    let stripped = normalized
        .strip_prefix('W')
        .map(str::trim_start)
        .or_else(|| normalized.strip_prefix('w').map(str::trim_start))
        .unwrap_or(normalized.as_str());
    if let Some(captures) = re_price_word().captures(stripped) {
        let value = parse_scaled_decimal(captures.get(2)?.as_str())?;
        let leading_minus = captures.get(1).map(|m| m.as_str() == "-").unwrap_or(false);
        let trailing_minus = captures.get(3).map(|m| m.as_str() == "-").unwrap_or(false);
        let is_negative = leading_minus || trailing_minus;
        return Some(if is_negative { -value } else { value });
    }
    if stripped.contains('@') || stripped.contains('/') {
        return None;
    }
    let captures = re_embedded_trailing_price_word().captures(stripped)?;
    parse_scaled_decimal(captures.get(1)?.as_str())
}

fn is_short_alpha_item(text: &str) -> bool {
    let letters_only: String = text.chars().filter(|ch| ch.is_ascii_alphabetic()).collect();
    letters_only.len() >= 3 && letters_only.chars().all(|ch| ch.is_ascii_alphabetic())
}

fn is_valid_onsale_target(line: &ParsedLine) -> bool {
    if line.left_text.is_empty() {
        return false;
    }
    if receipt_metadata_like(&line.left_text) || receipt_metadata_like(&line.full_text) {
        return false;
    }
    if is_summary_line(&line.left_text) || is_summary_line(&line.full_text) {
        return false;
    }
    if is_section_header_text(&line.left_text) || is_section_header_text(&line.full_text) {
        return false;
    }
    if looks_like_quantity_expression(&line.left_text) {
        return false;
    }
    if line_has_trailing_price(&line.full_text) {
        return false;
    }
    let stripped = strip_leading_receipt_codes(&line.left_text);
    !stripped.is_empty() && alpha_ratio(&stripped) >= 0.5
}

/// Per-character advance of the body font, as a fraction of image width.
///
/// Same measurement as `ocr_line_grouping::glyph_pitch` and for the same reason —
/// a character cell is the only stable unit for indentation — but taken from the
/// normalized bboxes this stage works in. Rows shorter than six characters are
/// mostly box padding; the modal height band keeps a double-width SUBTOTAL or a
/// display banner off the estimate.
fn glyph_pitch_normalized(doc: &OcrDocument) -> Option<f64> {
    let mut heights: Vec<f64> = doc
        .lines
        .iter()
        .flat_map(|line| line.words.iter())
        .map(|word| word.bbox.bottom - word.bbox.top)
        .filter(|height| *height > 0.0)
        .collect();
    if heights.is_empty() {
        return None;
    }
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_height = heights[heights.len() / 2];

    let mut pitches: Vec<f64> = Vec::new();
    for word in doc.lines.iter().flat_map(|line| line.words.iter()) {
        let chars = word.text.trim().chars().count();
        let width = word.bbox.right - word.bbox.left;
        let height = word.bbox.bottom - word.bbox.top;
        if chars < 6 || width <= 0.0 || height <= 0.0 {
            continue;
        }
        if height < median_height * 0.75 || height > median_height * 1.25 {
            continue;
        }
        pitches.push(width / chars as f64);
    }
    if pitches.len() < 5 {
        return None;
    }
    pitches.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    Some(pitches[pitches.len() / 2])
}

/// Deny itemhood to rows standing in a print-grid column that never carries a
/// price.
///
/// A receipt's annotation columns — Food Basics' `Saving 4.72` one cell in from
/// its items — are recognisable without knowing a single chain's vocabulary,
/// because they have three properties together: the column holds no priced row
/// anywhere on the receipt, it is deeper than the shallowest column that does,
/// and its rows carry their own amount inline. The last one is what separates an
/// annotation from a row whose price the grouper simply lost; it is a numeric
/// shape, not a keyword, so it stays merchant-blind.
///
/// Order matters. This must run on the pairing the *yield* produced
/// (`ocr_line_grouping::yields_to_price_column`), never on the raw one: a
/// savings row that has wrongly claimed a summary amount makes its own column
/// look priced and shields itself from this test. On the Food Basics receipt
/// that is exactly what happened — `Saving 2.01` held SUBTOTAL's 6.96, so the
/// annotation column was invisible until the yield took it back.
///
/// Measured over the 123-receipt corpus this denies 14 rows, none of which any
/// fixture asserts as an item; they are quantity breakdowns (`2 @ $5.99`) and
/// summary asides (`AMOUNT: $25.00`, `Eligible amount for point calculation`).
/// Both parser paths need this verdict, so it is computed from the geometry
/// alone and returned per line of the flattened page sequence — the spatial
/// extractor consumes it below, and `parser` withholds the same lines
/// from the text path, which has no coordinates of its own to decide with.
pub fn annotation_line_flags(doc: &OcrDocument) -> Vec<bool> {
    let rows: Vec<AnnotationRow> = doc.lines.iter().map(annotation_row).collect();
    let mut flags = vec![false; rows.len()];
    let Some(pitch) = glyph_pitch_normalized(doc) else {
        return flags;
    };
    if pitch <= 0.0 || rows.is_empty() {
        return flags;
    }

    let mut order: Vec<usize> = (0..rows.len())
        .filter(|&index| rows[index].left_x.is_finite())
        .collect();
    order.sort_by(|&a, &b| {
        rows[a]
            .left_x
            .partial_cmp(&rows[b].left_x)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    if order.is_empty() {
        return flags;
    }

    // Cluster left edges into columns by single linkage at half a cell. As in
    // the grouper, chaining is the safe failure: a ragged left column collapses
    // to one level, no column is deeper than the shallowest priced one, and
    // nothing is marked.
    let mut level = vec![usize::MAX; rows.len()];
    let mut current = 0usize;
    for (rank, &index) in order.iter().enumerate() {
        if rank > 0
            && rows[index].left_x - rows[order[rank - 1]].left_x > ANNOTATION_COLUMN_LINK * pitch
        {
            current += 1;
        }
        level[index] = current;
    }

    let mut priced_columns: Vec<bool> = vec![false; current + 1];
    for (index, row) in rows.iter().enumerate() {
        if row.has_price && level[index] != usize::MAX {
            priced_columns[level[index]] = true;
        }
    }
    let Some(shallowest_priced) = priced_columns.iter().position(|priced| *priced) else {
        return flags;
    };

    for (index, row) in rows.iter().enumerate() {
        let column = level[index];
        if column == usize::MAX || column <= shallowest_priced || priced_columns[column] {
            continue;
        }
        if row.left_text.is_empty() || !re_trailing_price().is_match(&row.left_text) {
            continue;
        }
        flags[index] = true;
    }
    flags
}

fn annotation_row(line: &OcrLine) -> AnnotationRow {
    let mut left_x = f64::INFINITY;
    let mut left_words: Vec<&str> = Vec::new();
    let mut has_price = false;
    for word in &line.words {
        let x = x_center(word);
        if x < PRICE_X_THRESHOLD {
            let text = word.text.as_str();
            if text.len() <= 1 || re_digits_dots_only().is_match(text) {
                continue;
            }
            left_words.push(text);
            left_x = left_x.min(word.bbox.left);
        } else if word.confidence >= MIN_CONFIDENCE
            && is_price_word(&word.text).is_some_and(|scaled| scaled != 0)
        {
            has_price = true;
        }
    }
    AnnotationRow {
        left_x,
        left_text: left_words.join(" "),
        has_price,
    }
}

fn is_valid_item_line(line: &ParsedLine, total_line_y: Option<f64>) -> bool {
    if line.is_annotation {
        return false;
    }
    let left_text_for_ratio = strip_leading_receipt_codes(&line.left_text);
    if left_text_for_ratio.is_empty() || line.left_text.is_empty() {
        return false;
    }
    if receipt_metadata_like(&line.left_text) || receipt_metadata_like(&line.full_text) {
        return false;
    }
    let short_alpha = is_short_alpha_item(&left_text_for_ratio);
    if line.left_text.len() < 5
        && !is_priced_generic_item_label(&line.left_text, &line.full_text)
        && !short_alpha
    {
        return false;
    }
    if let Some(total_y) = total_line_y {
        if line.line_y > total_y + Y_TOLERANCE {
            return false;
        }
    }
    if is_summary_line(&line.left_text) || is_summary_line(&line.full_text) {
        return false;
    }
    let left_is_header = is_section_header_text(&line.left_text)
        && !is_priced_generic_item_label(&line.left_text, &line.full_text);
    if left_is_header || is_section_header_text(&line.full_text) {
        return false;
    }
    if re_long_digits_only().is_match(&line.full_text) {
        return false;
    }
    let is_costco_discount = re_costco_discount_line().is_match(&left_text_for_ratio);
    if !is_costco_discount && alpha_ratio(&left_text_for_ratio) < 0.5 {
        return false;
    }
    if re_malformed_ocr_prefix().is_match(&line.left_text) {
        return false;
    }
    if re_mangled_reg_marker().is_match(line.left_text.trim()) {
        return false;
    }
    if line.left_text.len() < 8
        && !line.left_text.contains(' ')
        && !is_priced_generic_item_label(&line.left_text, &line.full_text)
        && !short_alpha
    {
        return false;
    }
    if footer_address_like(&line.full_text) {
        return false;
    }
    if looks_like_onsale_marker(&line.left_text) {
        return false;
    }
    if re_multibuy_parenthetical().is_match(&line.left_text) {
        return false;
    }
    if re_short_parenthetical_code().is_match(&line.left_text)
        && line.left_text.len() < 12
        && !is_short_alpha_item(&clean_description(&line.left_text))
    {
        // "(4001)"-style code stubs are not items, but a short name behind a
        // strippable promo marker — e.g. T&T's "(SALE) NAPA" — still is.
        return false;
    }
    if re_weight_info_line().is_match(line.left_text.trim()) {
        return false;
    }
    true
}

/// Whether a weighed qty row's own math (`<w> kg @ $<u>/kg`) reproduces
/// `price_scaled`. `None` when the row isn't that shape. `Some(false)` means
/// the trailing price drifted in from another row during line grouping.
fn weight_row_price_reconciles(left_text: &str, price_scaled: i64) -> Option<bool> {
    let captures = re_weight_at_unit_price().captures(left_text.trim())?;
    let weight: f64 = captures.get(1)?.as_str().parse().ok()?;
    let unit: f64 = captures.get(2)?.as_str().parse().ok()?;
    let expected_cents = (weight * unit * 100.0).round() as i64;
    let price_cents = (price_scaled as f64 / 100.0).round() as i64;
    Some((expected_cents - price_cents).abs() <= 1)
}

fn has_nearby_quantity_expression_above(all_lines: &[ParsedLine], line_index: usize) -> bool {
    let anchor_y = all_lines[line_index].line_y;
    all_lines
        .iter()
        .enumerate()
        .filter(|(index, candidate)| {
            *index != line_index
                && candidate.line_y < anchor_y
                && anchor_y - candidate.line_y <= MAX_ITEM_DISTANCE
        })
        .max_by(|(_, left), (_, right)| left.line_y.partial_cmp(&right.line_y).unwrap())
        .is_some_and(|(_, candidate)| looks_like_quantity_expression(&candidate.left_text))
}

fn nearest_unpriced_deposit_stub_below(
    all_lines: &[ParsedLine],
    line_index: usize,
    used_line_indices: &[bool],
) -> Option<(usize, f64)> {
    let anchor_y = all_lines[line_index].line_y;
    let nearest_below = all_lines
        .iter()
        .enumerate()
        .filter(|(index, candidate)| {
            *index != line_index
                && candidate.line_y > anchor_y
                && candidate.line_y - anchor_y <= MAX_ITEM_DISTANCE
        })
        .min_by(|(_, left), (_, right)| left.line_y.partial_cmp(&right.line_y).unwrap())?;
    let (index, candidate) = nearest_below;
    if used_line_indices[index]
        || !is_deposit_stub(&candidate.left_text)
        || line_has_trailing_price(&candidate.full_text)
    {
        return None;
    }
    Some((index, candidate.line_y - anchor_y))
}

fn y_center(word: &OcrWord) -> f64 {
    (word.bbox.top + word.bbox.bottom) / 2.0
}

fn x_center(word: &OcrWord) -> f64 {
    (word.bbox.left + word.bbox.right) / 2.0
}

pub fn extract_spatial_items(doc: &OcrDocument) -> SpatialExtractionOutcome {
    let mut items = Vec::new();
    let mut warnings = Vec::new();

    let mut all_lines = Vec::new();
    let mut price_candidates = Vec::new();

    for line in &doc.lines {
        if line.words.is_empty() {
            continue;
        }
        let full_text = line.text.clone();
        let line_has_price = line_has_trailing_price(&full_text);
        let mut left_words = Vec::new();
        let mut left_y = None;
        for word in &line.words {
            let x = x_center(word);
            // PRICE_X_THRESHOLD is the description/price boundary;
            // there's no dead zone (Costco's "2% 4L" pack-size token
            // sits at cx≈0.6 and must count as description text).
            if x < PRICE_X_THRESHOLD {
                let text = word.text.as_str();
                if text.len() <= 1 || re_digits_dots_only().is_match(text) {
                    continue;
                }
                if is_section_header_text(text) && !line_has_price {
                    continue;
                }
                left_words.push(text.to_string());
                if left_y.is_none() {
                    left_y = Some(y_center(word));
                }
            }
        }
        let line_y = left_y.unwrap_or_else(|| y_center(&line.words[0]));
        let line_index = all_lines.len();
        all_lines.push(ParsedLine {
            line_y,
            full_text: full_text.clone(),
            left_text: left_words.join(" "),
            is_annotation: false,
        });
        for word in &line.words {
            if word.confidence < MIN_CONFIDENCE {
                continue;
            }
            let x = x_center(word);
            if x <= PRICE_X_THRESHOLD {
                continue;
            }
            if let Some(price_scaled) = is_price_word(&word.text) {
                if price_scaled != 0 {
                    price_candidates.push(PriceCandidate {
                        price_y: y_center(word),
                        price_scaled,
                        source_line_index: line_index,
                    });
                }
            }
        }
    }

    for (line, is_annotation) in all_lines.iter_mut().zip(annotation_line_flags(doc)) {
        line.is_annotation = is_annotation;
    }

    let total_line_y = all_lines
        .iter()
        .filter(|line| {
            let upper = line.full_text.to_ascii_uppercase();
            upper.contains("TOTAL")
                && !upper.contains("SUBTOTAL")
                && !upper.contains("TOTAL NUMBER")
                && !upper.contains("TOTAL DISCOUNT")
                && !upper.contains("TOTAL ITEMS")
                && !upper.contains("TOTAL SAVINGS")
                && !upper.contains("TOTAL SAVED")
        })
        .map(|line| line.line_y)
        .min_by(|a, b| a.partial_cmp(b).unwrap());

    let mut used_line_indices = vec![false; all_lines.len()];

    for price_candidate in price_candidates {
        let price_y = price_candidate.price_y;
        if let Some(total_y) = total_line_y {
            if price_y > total_y + Y_TOLERANCE {
                continue;
            }
        }
        if all_lines.is_empty() {
            continue;
        }

        let closest_line_index = all_lines
            .iter()
            .enumerate()
            .min_by(|(_, left), (_, right)| {
                (left.line_y - price_y)
                    .abs()
                    .partial_cmp(&(right.line_y - price_y).abs())
                    .unwrap()
            })
            .map(|(index, _)| index);
        let Some(closest_line_index) = closest_line_index else {
            continue;
        };
        let source_line = &all_lines[price_candidate.source_line_index];
        let closest_line = &all_lines[closest_line_index];

        let context_full_text = if source_line.full_text.is_empty() {
            &closest_line.full_text
        } else {
            &source_line.full_text
        };
        let context_left_text = if source_line.left_text.is_empty() {
            &closest_line.left_text
        } else {
            &source_line.left_text
        };
        let full_upper = context_full_text.to_ascii_uppercase();
        let price_line_has_onsale = looks_like_onsale_marker(&full_upper);
        let left_is_header = is_section_header_text(context_left_text)
            && !is_priced_generic_item_label(context_left_text, context_full_text);
        let mut prefer_below = left_is_header
            || is_section_header_text(context_full_text)
            || context_left_text.is_empty();
        if price_line_has_onsale {
            prefer_below = true;
        }

        let mut is_summary = false;
        if let Some(total_y) = total_line_y {
            if price_y > total_y - MAX_ITEM_DISTANCE {
                for candidate in &all_lines {
                    if (candidate.line_y - price_y).abs() > Y_TOLERANCE {
                        continue;
                    }
                    if candidate.line_y > price_y + SPATIAL_FLOAT_EPSILON {
                        continue;
                    }
                    if is_summary_line(&candidate.left_text)
                        || is_summary_line(&candidate.full_text)
                    {
                        is_summary = true;
                        break;
                    }
                }
            }
        }

        if !is_summary {
            let full_text_stripped = closest_line.full_text.trim();
            if is_summary_line(&closest_line.left_text) || is_summary_line(&closest_line.full_text)
            {
                is_summary = true;
            } else if re_standalone_price().is_match(full_text_stripped) {
                let nearest_above = all_lines
                    .iter()
                    .enumerate()
                    .filter(|(_, candidate)| candidate.line_y < closest_line.line_y)
                    .max_by(|(_, left), (_, right)| {
                        left.line_y.partial_cmp(&right.line_y).unwrap()
                    });
                if let Some((_, above)) = nearest_above {
                    if closest_line.line_y - above.line_y <= MAX_ITEM_DISTANCE
                        && (is_summary_line(&above.left_text) || is_summary_line(&above.full_text))
                    {
                        is_summary = true;
                    }
                }
                if !is_summary {
                    if let Some(total_y) = total_line_y {
                        if closest_line.line_y > total_y - MAX_ITEM_DISTANCE {
                            for candidate in &all_lines {
                                if (candidate.line_y - closest_line.line_y).abs()
                                    > MAX_ITEM_DISTANCE
                                {
                                    continue;
                                }
                                if is_summary_line(&candidate.left_text)
                                    || is_summary_line(&candidate.full_text)
                                {
                                    is_summary = true;
                                    break;
                                }
                            }
                        }
                    }
                }
            }
        }

        let mut onsale_target_line_index = None;
        if !is_summary && price_line_has_onsale {
            let anchor_y = source_line.line_y;
            let nearest_above = all_lines
                .iter()
                .enumerate()
                .filter(|(_, candidate)| {
                    candidate.line_y < anchor_y
                        && anchor_y - candidate.line_y <= MAX_ITEM_DISTANCE
                        && is_valid_onsale_target(candidate)
                })
                .max_by(|(_, left), (_, right)| left.line_y.partial_cmp(&right.line_y).unwrap());
            let nearest_below = all_lines
                .iter()
                .enumerate()
                .filter(|(_, candidate)| {
                    candidate.line_y > anchor_y
                        && candidate.line_y - anchor_y <= MAX_ITEM_DISTANCE
                        && is_valid_onsale_target(candidate)
                })
                .min_by(|(_, left), (_, right)| left.line_y.partial_cmp(&right.line_y).unwrap());
            match (nearest_above, nearest_below) {
                (Some((above_index, above)), Some((below_index, below))) => {
                    let above_distance = anchor_y - above.line_y;
                    let below_distance = below.line_y - anchor_y;
                    onsale_target_line_index = Some(if above_distance <= below_distance {
                        above_index
                    } else {
                        below_index
                    });
                }
                (Some((index, _)), None) | (None, Some((index, _))) => {
                    onsale_target_line_index = Some(index);
                }
                (None, None) => {
                    is_summary = true;
                }
            }
        }

        if is_summary {
            continue;
        }

        let line_selection_candidates = all_lines
            .iter()
            .enumerate()
            .map(|(index, line)| {
                SpatialLineCandidate::new(
                    line.line_y,
                    used_line_indices[index],
                    is_valid_item_line(line, total_line_y),
                    line_has_trailing_price(&line.full_text),
                    looks_like_quantity_expression(&line.left_text),
                )
            })
            .collect::<Vec<_>>();

        let mut found_item = false;
        let mut chosen_line_index = None;
        let mut chosen_distance = f64::INFINITY;
        let mut suppress_fallback_for_ambiguous_code_only_source = false;
        let selection_anchor_y = source_line.line_y;
        let source_line_is_quantity_expression =
            looks_like_quantity_expression(&source_line.left_text);
        let source_line_needs_item_context = lacks_description_context(&source_line.left_text);
        let source_line_repeats_previous_priced_item = source_line_needs_item_context
            && all_lines
                .iter()
                .enumerate()
                .filter(|(_, candidate)| {
                    candidate.line_y < selection_anchor_y
                        && selection_anchor_y - candidate.line_y <= MAX_ITEM_DISTANCE
                        && is_valid_item_line(candidate, total_line_y)
                        && line_has_trailing_price(&candidate.full_text)
                        && trailing_price_scaled(&candidate.full_text)
                            == Some(price_candidate.price_scaled)
                })
                .max_by(|(_, left), (_, right)| left.line_y.partial_cmp(&right.line_y).unwrap())
                .is_some();

        // A priced scale-weight row belongs to the produce label sitting above
        // its contiguous weight block, however tall the block is (No Frills
        // prints four gross/tare/net weighings under one "CHERRIES RED" label).
        // Content-verified, so it bypasses the geometric distance gates, and
        // the label is deliberately reusable across the block's weighings.
        if re_weight_info_line().is_match(source_line.left_text.trim()) {
            let mut j = price_candidate.source_line_index;
            while j > 0 {
                j -= 1;
                let candidate = &all_lines[j];
                let candidate_text = candidate.left_text.trim();
                if candidate_text.is_empty()
                    || re_weight_info_line().is_match(candidate_text)
                    || looks_like_quantity_expression(candidate_text)
                {
                    continue;
                }
                let cleaned = strip_leading_receipt_codes(candidate_text);
                if !is_section_header_text(candidate_text)
                    && !is_summary_line(candidate_text)
                    && !is_summary_line(&candidate.full_text)
                    && !line_has_trailing_price(&candidate.full_text)
                    && !cleaned.is_empty()
                    && alpha_ratio(&cleaned) >= 0.5
                {
                    chosen_line_index = Some(j);
                    chosen_distance = 0.0;
                }
                break;
            }
        }

        if source_line_is_quantity_expression && chosen_line_index.is_none() {
            let source_modifier = parse_quantity_modifier(&source_line.left_text);
            let mut nearest_unpriced_above = None;
            let mut nearest_unpriced_below = None;
            let mut nearest_priced_below_with_deposit_stub = None;
            // Deposit stubs (e.g. "DEPOSIT 1") are normally skipped so a regular
            // quantity expression like "3@$3.49" doesn't pair with a deposit label
            // above it.  But when a quantity expression IS for a deposit (e.g.
            // "2@$0.10 0.20"), the deposit stub immediately above IS the correct
            // target.  Track the closest unused deposit stub within Y_TOLERANCE so
            // we can fall back to it when no regular item is found above.
            let mut nearest_deposit_stub_above_within_tolerance: Option<(usize, f64)> = None;

            for (index, candidate) in all_lines.iter().enumerate() {
                if used_line_indices[index] || !is_valid_item_line(candidate, total_line_y) {
                    continue;
                }

                let distance = (candidate.line_y - selection_anchor_y).abs();
                if distance > MAX_ITEM_DISTANCE + SPATIAL_FLOAT_EPSILON {
                    continue;
                }

                let candidate_has_trailing_price = line_has_trailing_price(&candidate.full_text);
                if candidate_has_trailing_price {
                    if candidate.line_y > selection_anchor_y
                        && nearest_unpriced_deposit_stub_below(
                            &all_lines,
                            index,
                            &used_line_indices,
                        )
                        .is_some()
                    {
                        match nearest_priced_below_with_deposit_stub {
                            Some((_, current_distance)) if distance >= current_distance => {}
                            _ => nearest_priced_below_with_deposit_stub = Some((index, distance)),
                        }
                    }
                    continue;
                }

                if is_deposit_stub(&candidate.left_text) {
                    // Track closest deposit stub above within Y_TOLERANCE as a
                    // fallback for deposit-quantity expressions.
                    if candidate.line_y < selection_anchor_y
                        && distance <= Y_TOLERANCE + SPATIAL_FLOAT_EPSILON
                    {
                        match nearest_deposit_stub_above_within_tolerance {
                            Some((_, current_distance)) if distance >= current_distance => {}
                            _ => {
                                nearest_deposit_stub_above_within_tolerance =
                                    Some((index, distance))
                            }
                        }
                    }
                    continue;
                }

                if candidate.line_y < selection_anchor_y {
                    match nearest_unpriced_above {
                        Some((_, current_distance)) if distance >= current_distance => {}
                        _ => nearest_unpriced_above = Some((index, distance)),
                    }
                } else if candidate.line_y > selection_anchor_y {
                    match nearest_unpriced_below {
                        Some((_, current_distance)) if distance >= current_distance => {}
                        _ => nearest_unpriced_below = Some((index, distance)),
                    }
                }
            }

            // A weighed qty row usually sits under its item label, so with a
            // real quantity modifier the label above wins. But when the row's
            // own math does not reproduce the trailing price, that price
            // drifted in from another row during line grouping (No Frills'
            // "1.775 kg @ $1.52/kg" carrying the 5.99 of the melon below), so
            // fall back to plain nearest-line resolution.
            let own_price =
                weight_row_price_reconciles(&source_line.left_text, price_candidate.price_scaled);
            chosen_line_index = match (
                nearest_unpriced_above,
                nearest_unpriced_below,
                source_modifier && own_price != Some(false),
            ) {
                (Some((index, distance)), Some(_), true) => {
                    chosen_distance = distance;
                    Some(index)
                }
                (
                    Some((above_index, above_distance)),
                    Some((below_index, below_distance)),
                    false,
                ) => {
                    if above_distance <= below_distance {
                        chosen_distance = above_distance;
                        Some(above_index)
                    } else {
                        chosen_distance = below_distance;
                        Some(below_index)
                    }
                }
                (Some((index, distance)), None, _) => {
                    chosen_distance = distance;
                    Some(index)
                }
                // No regular item above: prefer a deposit stub within Y_TOLERANCE
                // over a non-deposit item below, so "2@$0.10" pairs with "DEPOSIT 1"
                // rather than the next real item below.
                (None, Some((below_index, below_distance)), _) => {
                    if let Some((stub_index, stub_distance)) =
                        nearest_deposit_stub_above_within_tolerance
                    {
                        chosen_distance = stub_distance;
                        Some(stub_index)
                    } else {
                        chosen_distance = below_distance;
                        Some(below_index)
                    }
                }
                (None, None, _) => nearest_priced_below_with_deposit_stub
                    .or(nearest_deposit_stub_above_within_tolerance)
                    .map(|(index, distance)| {
                        chosen_distance = distance;
                        index
                    }),
            };
        }

        if !prefer_below && source_line_is_quantity_expression {
            let mut nearest_same_row_above = None;
            let mut nearest_same_row_below = None;

            for (index, candidate) in all_lines.iter().enumerate() {
                if used_line_indices[index] || !is_valid_item_line(candidate, total_line_y) {
                    continue;
                }
                let distance = (candidate.line_y - selection_anchor_y).abs();
                if distance > Y_TOLERANCE + SPATIAL_FLOAT_EPSILON {
                    continue;
                }
                if candidate.line_y < selection_anchor_y {
                    match nearest_same_row_above {
                        Some(current_distance) if distance >= current_distance => {}
                        _ => nearest_same_row_above = Some(distance),
                    }
                } else if candidate.line_y > selection_anchor_y {
                    match nearest_same_row_below {
                        Some(current_distance) if distance >= current_distance => {}
                        _ => nearest_same_row_below = Some(distance),
                    }
                }
            }

            if nearest_same_row_below.is_some() && nearest_same_row_above.is_none() {
                prefer_below = true;
            }
        }

        let source_distance = (source_line.line_y - price_y).abs();
        let shifted_deposit_target = if source_distance <= Y_TOLERANCE
            && is_valid_item_line(source_line, total_line_y)
            && !looks_like_quantity_expression(&source_line.left_text)
            && has_nearby_quantity_expression_above(&all_lines, price_candidate.source_line_index)
        {
            nearest_unpriced_deposit_stub_below(
                &all_lines,
                price_candidate.source_line_index,
                &used_line_indices,
            )
        } else {
            None
        };

        if onsale_target_line_index.is_none()
            && !used_line_indices[price_candidate.source_line_index]
        {
            if shifted_deposit_target.is_none()
                && trailing_price_scaled(&source_line.full_text)
                    == Some(price_candidate.price_scaled)
                && is_valid_item_line(source_line, total_line_y)
                && !looks_like_quantity_expression(&source_line.left_text)
            {
                chosen_line_index = Some(price_candidate.source_line_index);
                chosen_distance = source_distance;
            } else if let Some((index, distance)) = shifted_deposit_target {
                chosen_line_index = Some(index);
                chosen_distance = distance;
            } else if source_distance <= Y_TOLERANCE
                && is_valid_item_line(source_line, total_line_y)
                && !looks_like_quantity_expression(&source_line.left_text)
            {
                chosen_line_index = Some(price_candidate.source_line_index);
                chosen_distance = source_distance;
            }
        }

        if chosen_line_index.is_none() {
            if let Some(index) = onsale_target_line_index {
                if !used_line_indices[index] {
                    chosen_line_index = Some(index);
                    chosen_distance = (all_lines[index].line_y - price_y).abs();
                }
            }
        }

        if chosen_line_index.is_none() {
            if let Some((index, distance)) = select_spatial_item_line(
                selection_anchor_y,
                Y_TOLERANCE,
                MAX_ITEM_DISTANCE,
                prefer_below,
                price_line_has_onsale,
                line_selection_candidates,
            ) {
                let selected_line = &all_lines[index];
                let selected_line_is_next_priced_row = source_line_needs_item_context
                    && selected_line.line_y > price_y + SPATIAL_FLOAT_EPSILON
                    && line_has_trailing_price(&selected_line.full_text);
                if selected_line_is_next_priced_row {
                    suppress_fallback_for_ambiguous_code_only_source = true;
                } else {
                    chosen_line_index = Some(index);
                    chosen_distance = distance;
                }
            }
        }

        if let Some(index) = chosen_line_index {
            let direct_match_tolerance = if source_line_is_quantity_expression || prefer_below {
                MAX_ITEM_DISTANCE + SPATIAL_FLOAT_EPSILON
            } else {
                Y_TOLERANCE + SPATIAL_FLOAT_EPSILON
            };
            if chosen_distance <= direct_match_tolerance {
                let description = clean_description(&all_lines[index].left_text);
                if description.len() > 2
                    && !re_mangled_reg_marker().is_match(all_lines[index].left_text.trim())
                    && !re_mangled_reg_marker().is_match(description.trim())
                {
                    used_line_indices[index] = true;
                    items.push(SpatialExtractedItem {
                        description,
                        price: Money::from_scaled_4(price_candidate.price_scaled),
                    });
                    found_item = true;
                }
            }
        }

        if !found_item
            && shifted_deposit_target.is_none()
            && !used_line_indices[price_candidate.source_line_index]
            && trailing_price_scaled(&source_line.full_text) == Some(price_candidate.price_scaled)
            && is_valid_item_line(source_line, total_line_y)
            && !looks_like_quantity_expression(&source_line.left_text)
        {
            let description = clean_description(&source_line.left_text);
            if description.len() > 2 && !re_mangled_reg_marker().is_match(description.trim()) {
                used_line_indices[price_candidate.source_line_index] = true;
                items.push(SpatialExtractedItem {
                    description,
                    price: Money::from_scaled_4(price_candidate.price_scaled),
                });
                found_item = true;
            }
        }

        if !found_item && !suppress_fallback_for_ambiguous_code_only_source {
            if source_line_repeats_previous_priced_item {
                continue;
            }
            let mut lines_above = all_lines
                .iter()
                .enumerate()
                .filter(|(_, line)| {
                    line.line_y < price_y - Y_TOLERANCE
                        && (price_y - line.line_y) <= MAX_ITEM_DISTANCE
                })
                .collect::<Vec<_>>();
            lines_above
                .sort_by(|(_, left), (_, right)| right.line_y.partial_cmp(&left.line_y).unwrap());

            for (index, line) in lines_above.into_iter().take(5) {
                if used_line_indices[index] {
                    continue;
                }
                if price_line_has_onsale && line_has_trailing_price(&line.full_text) {
                    continue;
                }
                if line.left_text.len() < 3 {
                    continue;
                }
                if is_summary_line(&line.left_text) || is_summary_line(&line.full_text) {
                    continue;
                }
                if re_weight_info().is_match(&line.full_text.to_ascii_lowercase()) {
                    continue;
                }
                if re_w_dollar().is_match(&line.full_text) {
                    continue;
                }
                if re_standalone_price().is_match(line.full_text.trim()) {
                    continue;
                }
                let left_is_header = is_section_header_text(&line.left_text)
                    && !is_priced_generic_item_label(&line.left_text, &line.full_text);
                if left_is_header || is_section_header_text(&line.full_text) {
                    continue;
                }
                let left_text_for_ratio = strip_leading_receipt_codes(&line.left_text);
                if left_text_for_ratio.is_empty() {
                    continue;
                }
                let is_costco_discount = re_costco_discount_line().is_match(&left_text_for_ratio);
                if !is_costco_discount && alpha_ratio(&left_text_for_ratio) < 0.4 {
                    continue;
                }
                let description = clean_description(&line.left_text);
                if description.len() > 2 && !re_mangled_reg_marker().is_match(description.trim()) {
                    used_line_indices[index] = true;
                    items.push(SpatialExtractedItem {
                        description,
                        price: Money::from_scaled_4(price_candidate.price_scaled),
                    });
                    found_item = true;
                    break;
                }
            }
        }

        if !found_item {
            let mut context_text = source_line.full_text.trim().to_string();
            if context_text.is_empty() {
                context_text = closest_line.full_text.trim().to_string();
            }
            // Truncate by characters, not bytes: byte 80 may split a multibyte
            // CJK char (Asian-grocery receipts) and panic.
            if let Some((byte_idx, _)) = context_text.char_indices().nth(80) {
                context_text.truncate(byte_idx);
            }
            let mut message = format!(
                "maybe missed item near price {}",
                Money::from_scaled_4(price_candidate.price_scaled)
            );
            if !context_text.is_empty() {
                message.push_str(&format!(" (context: \"{}\")", context_text));
            }
            warnings.push(SpatialParserWarning {
                kind: ReceiptWarningKind::PossibleMissedItem,
                message,
                after_item_index: if items.is_empty() {
                    None
                } else {
                    Some(items.len() - 1)
                },
            });
        }
    }

    SpatialExtractionOutcome { items, warnings }
}
