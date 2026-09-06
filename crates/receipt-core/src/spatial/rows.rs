//! Spatial extraction: rows.
use super::patterns::*;
use super::types::*;
use crate::ocr_document::{OcrDocument, OcrLine, OcrWord};
pub(super) fn normalize_decimal_spacing(text: &str) -> String {
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
pub(super) fn parse_scaled_decimal(token: &str) -> Option<i64> {
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
pub(super) fn alpha_ratio(value: &str) -> f64 {
    let non_ws_count = value.chars().filter(|ch| !ch.is_whitespace()).count();
    if non_ws_count == 0 {
        return 0.0;
    }
    let alpha_count = value.chars().filter(|ch| ch.is_ascii_alphabetic()).count();
    alpha_count as f64 / non_ws_count as f64
}
pub(super) fn is_section_name(text: &str) -> bool {
    matches!(
        text,
        "MEAT" | "SEAFOOD" | "PRODUCE" | "DELI" | "GROCERY" | "BAKERY" | "FROZEN" | "FOOD"
    )
}
pub(super) fn strip_leading_receipt_codes(text: &str) -> String {
    let trimmed = text.trim();
    let trimmed = re_leading_qty_prefix().replace(trimmed, "");
    let trimmed = re_leading_long_sku().replace(trimmed.as_ref(), "");
    let trimmed = re_leading_short_code().replace(trimmed.as_ref(), "$rest");
    let trimmed = re_leading_section_item_prefix().replace(trimmed.as_ref(), "");
    trimmed.trim().to_string()
}
pub(super) fn is_section_header_text(text: &str) -> bool {
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
pub(super) fn is_summary_line(text: &str) -> bool {
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
pub(super) fn trailing_price_scaled(text: &str) -> Option<i64> {
    let normalized = normalize_decimal_spacing(text.trim());
    let captures = re_trailing_price().captures(&normalized)?;
    let value = parse_scaled_decimal(captures.get(1)?.as_str())?;
    let is_negative = captures.get(2).map(|m| m.as_str() == "-").unwrap_or(false);
    Some(if is_negative { -value } else { value })
}
pub(super) fn line_has_trailing_price(text: &str) -> bool {
    trailing_price_scaled(text).is_some()
}
pub(super) fn looks_like_onsale_marker(text: &str) -> bool {
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
pub(super) fn is_priced_generic_item_label(left_text: &str, full_text: &str) -> bool {
    if left_text.trim().is_empty() {
        return false;
    }
    line_has_trailing_price(full_text)
        && matches!(
            left_text.trim().to_ascii_uppercase().as_str(),
            "MEAT" | "BAKERY"
        )
}
pub(super) fn parse_quantity_modifier(text: &str) -> bool {
    re_count_at_price().is_match(text)
        || re_weight_at_price().is_match(text)
        || re_multi_for_price().is_match(text)
}
pub(super) fn looks_like_quantity_expression(text: &str) -> bool {
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
pub(super) fn receipt_metadata_like(text: &str) -> bool {
    re_receipt_metadata_patterns().is_match(text.trim())
}
pub(super) fn clean_description(desc: &str) -> String {
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
pub(super) fn is_deposit_stub(text: &str) -> bool {
    let cleaned = clean_description(text);
    let upper = cleaned.to_ascii_uppercase();
    upper == "DEPOSIT" || upper.starts_with("DEPOSIT ")
}
pub(super) fn lacks_description_context(text: &str) -> bool {
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
pub(super) fn is_short_alpha_item(text: &str) -> bool {
    let letters_only: String = text.chars().filter(|ch| ch.is_ascii_alphabetic()).collect();
    letters_only.len() >= 3 && letters_only.chars().all(|ch| ch.is_ascii_alphabetic())
}
pub(super) fn is_valid_onsale_target(line: &ParsedLine) -> bool {
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
pub(super) fn glyph_pitch_normalized(doc: &OcrDocument) -> Option<f64> {
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
pub(super) fn annotation_row(line: &OcrLine) -> AnnotationRow {
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
pub(super) fn is_valid_item_line(line: &ParsedLine, total_line_y: Option<f64>) -> bool {
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
pub(super) fn weight_row_price_reconciles(left_text: &str, price_scaled: i64) -> Option<bool> {
    let captures = re_weight_at_unit_price().captures(left_text.trim())?;
    let weight: f64 = captures.get(1)?.as_str().parse().ok()?;
    let unit: f64 = captures.get(2)?.as_str().parse().ok()?;
    let expected_cents = (weight * unit * 100.0).round() as i64;
    let price_cents = (price_scaled as f64 / 100.0).round() as i64;
    Some((expected_cents - price_cents).abs() <= 1)
}
pub(super) fn has_nearby_quantity_expression_above(
    all_lines: &[ParsedLine],
    line_index: usize,
) -> bool {
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
pub(super) fn nearest_unpriced_deposit_stub_below(
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
pub(super) fn y_center(word: &OcrWord) -> f64 {
    (word.bbox.top + word.bbox.bottom) / 2.0
}
pub(super) fn x_center(word: &OcrWord) -> f64 {
    (word.bbox.left + word.bbox.right) / 2.0
}

/// The row table every pairing stage reads: the rows themselves, which of them
/// have already been claimed, and where the summary block begins.
///
/// These three always travel together — a stage that asks "is this row eligible"
/// needs all three to answer — so they are one parameter rather than three.
/// Borrowed immutably, and rebuilt per price candidate, because claiming a row
/// mutates `used` between candidates.
#[derive(Clone, Copy)]
pub(crate) struct Rows<'a> {
    pub(crate) all: &'a [ParsedLine],
    pub(crate) used: &'a [bool],
    pub(crate) total_line_y: Option<f64>,
}

/// Stage 1 — read the document into rows and price candidates.
///
/// Every line becomes a [`ParsedLine`] whose `left_text` is the description
/// column only; every price-shaped word right of the column boundary becomes a
/// [`PriceCandidate`] remembering which row it was printed on. The two are
/// deliberately separate: which row a price was *printed* on is not which row it
/// *belongs* to, and the whole of stage 3 exists to tell them apart.
pub(super) fn classify_rows(doc: &OcrDocument) -> (Vec<ParsedLine>, Vec<PriceCandidate>) {
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

    (all_lines, price_candidates)
}

/// Stage 2 — the y of the receipt's own TOTAL row, if it printed one.
///
/// Everything below it is summary arithmetic rather than items, so this is the
/// floor stage 3 tests prices against. The exclusions are the other rows that
/// contain the word: `SUBTOTAL`, and the "TOTAL <noun>" counters that report how
/// many items or how much was saved. Topmost wins — a receipt that prints TOTAL
/// twice (a card slip under the itemization) means the first one.
pub(super) fn total_line_y(all_lines: &[ParsedLine]) -> Option<f64> {
    all_lines
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
        .min_by(|a, b| a.partial_cmp(b).unwrap())
}
