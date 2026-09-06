//! Spatial extraction: pairing.
use super::candidate::{select_spatial_item_line, SpatialLineCandidate};
use super::patterns::*;
use super::rows::*;
use super::types::*;
use crate::common::ReceiptWarningKind;
use crate::money::Money;

/// Stage 3a — is this price part of the receipt's summary block rather than an
/// item?
///
/// Three separate readings, in order, because a summary amount can be recognized
/// by any one of them and the cheapest comes first:
///
/// 1. **The price's own row is labelled.** Only rows at or *above* the price are
///    considered — a label printed below it belongs to the next amount, not this
///    one — and only once the price is within `MAX_ITEM_DISTANCE` of TOTAL.
/// 2. **The row it pairs with is labelled.**
/// 3. **The row it pairs with is a bare price** with no label of its own, in
///    which case the label is inferred from the row above it, or from any
///    summary row nearby when the price sits in the TOTAL band. This is the
///    case that catches a summary column whose labels OCR read onto separate
///    rows.
pub(super) fn price_is_summary(
    price_y: f64,
    closest_line: &ParsedLine,
    all_lines: &[ParsedLine],
    total_line_y: Option<f64>,
) -> bool {
    if let Some(total_y) = total_line_y {
        if price_y > total_y - MAX_ITEM_DISTANCE {
            for candidate in all_lines {
                if (candidate.line_y - price_y).abs() > Y_TOLERANCE {
                    continue;
                }
                if candidate.line_y > price_y + SPATIAL_FLOAT_EPSILON {
                    continue;
                }
                if is_summary_line(&candidate.left_text) || is_summary_line(&candidate.full_text) {
                    return true;
                }
            }
        }
    }

    if is_summary_line(&closest_line.left_text) || is_summary_line(&closest_line.full_text) {
        return true;
    }

    if !re_standalone_price().is_match(closest_line.full_text.trim()) {
        return false;
    }

    let nearest_above = all_lines
        .iter()
        .filter(|candidate| candidate.line_y < closest_line.line_y)
        .max_by(|left, right| left.line_y.partial_cmp(&right.line_y).unwrap());
    if let Some(above) = nearest_above {
        if closest_line.line_y - above.line_y <= MAX_ITEM_DISTANCE
            && (is_summary_line(&above.left_text) || is_summary_line(&above.full_text))
        {
            return true;
        }
    }

    if let Some(total_y) = total_line_y {
        if closest_line.line_y > total_y - MAX_ITEM_DISTANCE {
            for candidate in all_lines {
                if (candidate.line_y - closest_line.line_y).abs() > MAX_ITEM_DISTANCE {
                    continue;
                }
                if is_summary_line(&candidate.left_text) || is_summary_line(&candidate.full_text) {
                    return true;
                }
            }
        }
    }

    false
}

/// Stage 3b — which row an on-sale price is discounting.
///
/// A sale marker ("2 FOR", "WAS 4.99") prints on its own row between the item
/// and the shelf-price column, so the row nearest the marker in *either*
/// direction owns the amount — unlike the ordinary case, which prefers the row
/// above. Ties go to the row above.
pub(super) fn onsale_target(anchor_y: f64, all_lines: &[ParsedLine]) -> Option<usize> {
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
            Some(if above_distance <= below_distance {
                above_index
            } else {
                below_index
            })
        }
        (Some((index, _)), None) | (None, Some((index, _))) => Some(index),
        (None, None) => None,
    }
}

/// Stage 4 — which row this price belongs to.
///
/// The heart of the extractor, and the reason it is long: a price is claimed by
/// a cascade of increasingly weak rules, each of which is the *right* answer for
/// a layout some chain actually prints, and the first one to produce a row wins.
/// In order: a scale-weight block's label above it; the row a quantity
/// expression modifies; a deposit stub the price shifted onto; the row the price
/// is printed on, when its own trailing price agrees; the on-sale target from
/// stage 3b; and finally the generic nearest-eligible-row search in
/// [`candidate`](super::candidate).
///
/// `prefer_below` comes in from stage 3 and can be *raised* here — a quantity
/// expression with an eligible row below it and none above flips it — which is
/// why it is returned rather than read again by the caller.
pub(super) fn select_target_line(
    price_candidate: &PriceCandidate,
    price_y: f64,
    source_line: &ParsedLine,
    rows: Rows<'_>,
    mut prefer_below: bool,
    price_line_has_onsale: bool,
    onsale_target_line_index: Option<usize>,
) -> LineChoice {
    let line_selection_candidates = rows
        .all
        .iter()
        .enumerate()
        .map(|(index, line)| {
            SpatialLineCandidate::new(
                line.line_y,
                rows.used[index],
                is_valid_item_line(line, rows.total_line_y),
                line_has_trailing_price(&line.full_text),
                looks_like_quantity_expression(&line.left_text),
            )
        })
        .collect::<Vec<_>>();

    let mut chosen_line_index = None;
    let mut chosen_distance = f64::INFINITY;
    let mut suppress_fallback_for_ambiguous_code_only_source = false;
    let selection_anchor_y = source_line.line_y;
    let source_line_is_quantity_expression = looks_like_quantity_expression(&source_line.left_text);
    let source_line_needs_item_context = lacks_description_context(&source_line.left_text);
    let source_line_repeats_previous_priced_item = source_line_needs_item_context
        && rows
            .all
            .iter()
            .enumerate()
            .filter(|(_, candidate)| {
                candidate.line_y < selection_anchor_y
                    && selection_anchor_y - candidate.line_y <= MAX_ITEM_DISTANCE
                    && is_valid_item_line(candidate, rows.total_line_y)
                    && line_has_trailing_price(&candidate.full_text)
                    && trailing_price_scaled(&candidate.full_text)
                        == Some(price_candidate.price_scaled)
            })
            .max_by(|(_, left), (_, right)| left.line_y.partial_cmp(&right.line_y).unwrap())
            .is_some();

    // A priced scale-weight row belongs to the produce label above its block.
    if re_weight_info_line().is_match(source_line.left_text.trim()) {
        if let Some(index) = weight_block_label(price_candidate.source_line_index, rows.all) {
            chosen_line_index = Some(index);
            chosen_distance = 0.0;
        }
    }

    if source_line_is_quantity_expression && chosen_line_index.is_none() {
        if let Some((index, distance)) =
            quantity_expression_target(price_candidate, source_line, selection_anchor_y, rows)
        {
            chosen_line_index = Some(index);
            chosen_distance = distance;
        }
    }

    if !prefer_below && source_line_is_quantity_expression {
        prefer_below = quantity_row_prefers_below(selection_anchor_y, rows);
    }

    let source_distance = (source_line.line_y - price_y).abs();
    let shifted_deposit_target = if source_distance <= Y_TOLERANCE
        && is_valid_item_line(source_line, rows.total_line_y)
        && !looks_like_quantity_expression(&source_line.left_text)
        && has_nearby_quantity_expression_above(rows.all, price_candidate.source_line_index)
    {
        nearest_unpriced_deposit_stub_below(rows.all, price_candidate.source_line_index, rows.used)
    } else {
        None
    };

    if onsale_target_line_index.is_none() && !rows.used[price_candidate.source_line_index] {
        if shifted_deposit_target.is_none()
            && trailing_price_scaled(&source_line.full_text) == Some(price_candidate.price_scaled)
            && is_valid_item_line(source_line, rows.total_line_y)
            && !looks_like_quantity_expression(&source_line.left_text)
        {
            chosen_line_index = Some(price_candidate.source_line_index);
            chosen_distance = source_distance;
        } else if let Some((index, distance)) = shifted_deposit_target {
            chosen_line_index = Some(index);
            chosen_distance = distance;
        } else if source_distance <= Y_TOLERANCE
            && is_valid_item_line(source_line, rows.total_line_y)
            && !looks_like_quantity_expression(&source_line.left_text)
        {
            chosen_line_index = Some(price_candidate.source_line_index);
            chosen_distance = source_distance;
        }
    }

    if chosen_line_index.is_none() {
        if let Some(index) = onsale_target_line_index {
            if !rows.used[index] {
                chosen_line_index = Some(index);
                chosen_distance = (rows.all[index].line_y - price_y).abs();
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
            let selected_line = &rows.all[index];
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

    LineChoice {
        line_index: chosen_line_index,
        distance: chosen_distance,
        prefer_below,
        suppress_fallback: suppress_fallback_for_ambiguous_code_only_source,
        source_repeats_previous_priced_item: source_line_repeats_previous_priced_item,
        shifted_to_deposit_stub: shifted_deposit_target.is_some(),
    }
}

/// The item description a row yields, or `None` if the row cannot name one.
///
/// Two vetoes, and they are not the same test: a row must clean up to more than
/// two characters, and the cleaned text must not be a mangled `REG` marker —
/// OCR reads Loblaws' regular-price annotation as enough different things
/// (`REG`, `RE6`, `R£G`) that a shape test is the only way to catch it.
pub(super) fn row_description(line: &ParsedLine) -> Option<String> {
    let description = clean_description(&line.left_text);
    (description.len() > 2 && !re_mangled_reg_marker().is_match(description.trim()))
        .then_some(description)
}

/// Stage 5's last resort — the nearest row above that could name an item.
///
/// Reached only when every rule in stage 4 declined, so it is deliberately
/// permissive about *where* (up to `MAX_ITEM_DISTANCE`, five rows) and
/// deliberately strict about *what*: it walks upward and rejects rows that are
/// summary lines, section headers, weight or unit-price subtext, bare prices, or
/// mostly non-alphabetic. The alpha-ratio floor is waived for Costco's `TPD/`
/// discount rows, which are legitimately almost all digits.
pub(super) fn nearest_describable_row_above(
    price_y: f64,
    all_lines: &[ParsedLine],
    used_line_indices: &[bool],
    price_line_has_onsale: bool,
) -> Option<(usize, String)> {
    let mut lines_above = all_lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            line.line_y < price_y - Y_TOLERANCE && (price_y - line.line_y) <= MAX_ITEM_DISTANCE
        })
        .collect::<Vec<_>>();
    lines_above.sort_by(|(_, left), (_, right)| right.line_y.partial_cmp(&left.line_y).unwrap());

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
        if let Some(description) = row_description(line) {
            return Some((index, description));
        }
    }
    None
}

/// The warning raised when a price could not be paired with any row.
///
/// It carries the surrounding text because the amount alone is not actionable —
/// "maybe missed item near price 4.99" on a 40-line receipt names nothing. The
/// context is the price's own row, or the row it landed nearest when its own is
/// blank.
pub(super) fn missed_item_warning(
    price_candidate: &PriceCandidate,
    source_line: &ParsedLine,
    closest_line: &ParsedLine,
    item_count: usize,
) -> SpatialParserWarning {
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
    SpatialParserWarning {
        kind: ReceiptWarningKind::PossibleMissedItem,
        message,
        after_item_index: item_count.checked_sub(1),
    }
}

/// The row whose center sits nearest this price vertically.
///
/// Not necessarily the row that owns it — that is stage 4's whole job — but the
/// row whose text describes the price's neighbourhood, which is what stages 3
/// and 5 read for context.
pub(super) fn closest_row(price_y: f64, all_lines: &[ParsedLine]) -> Option<usize> {
    all_lines
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| {
            (left.line_y - price_y)
                .abs()
                .partial_cmp(&(right.line_y - price_y).abs())
                .unwrap()
        })
        .map(|(index, _)| index)
}

/// What the text around a price says about how to pair it.
pub(super) struct PriceContext {
    /// The context row carries a sale marker, which changes both the target
    /// search (stage 3b) and the direction preference.
    pub(super) price_line_has_onsale: bool,
    /// Look *below* the price for its item rather than above. True when the
    /// context row cannot itself be an item — a department header, or a row with
    /// no description column at all — because then the price is heading a group
    /// rather than trailing one.
    pub(super) prefer_below: bool,
}

/// Stage 3 — read the text around a price.
///
/// The context is the price's own row, falling back to the nearest row when its
/// own is blank on that field. The two fields fall back independently on
/// purpose: a row can have geometry in the description column and no text, and
/// vice versa.
pub(super) fn price_context(source_line: &ParsedLine, closest_line: &ParsedLine) -> PriceContext {
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
    let price_line_has_onsale = looks_like_onsale_marker(&context_full_text.to_ascii_uppercase());
    let left_is_header = is_section_header_text(context_left_text)
        && !is_priced_generic_item_label(context_left_text, context_full_text);
    PriceContext {
        price_line_has_onsale,
        prefer_below: price_line_has_onsale
            || left_is_header
            || is_section_header_text(context_full_text)
            || context_left_text.is_empty(),
    }
}

/// The produce label above a contiguous block of scale-weight rows.
///
/// No Frills prints four gross/tare/net weighings under one "CHERRIES RED"
/// label, so the label can be several rows up and is reused by every weighing in
/// the block. Content-verified rather than distance-gated for exactly that
/// reason, and it walks up only until the first row that is neither blank, a
/// weight row, nor a quantity expression — that row either is the label or there
/// is none.
pub(super) fn weight_block_label(
    source_line_index: usize,
    all_lines: &[ParsedLine],
) -> Option<usize> {
    let mut j = source_line_index;
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
            return Some(j);
        }
        break;
    }
    None
}

/// The row a quantity expression modifies.
///
/// A row like `3 @ $3.49` or `1.775 kg @ $1.52/kg` prices *another* row, and
/// which one depends on what is around it: with a real quantity modifier the
/// label above wins, but when the row's own arithmetic does not reproduce the
/// trailing price the price drifted in from elsewhere during line grouping, so
/// plain nearest-row resolution takes over.
///
/// Deposit stubs are the exception threaded through this: normally skipped, so
/// `3@$3.49` does not pair with a "DEPOSIT 1" label above it — but when the
/// quantity expression is itself a deposit (`2@$0.10 0.20`), that stub is
/// exactly the right target.
pub(super) fn quantity_expression_target(
    price_candidate: &PriceCandidate,
    source_line: &ParsedLine,
    selection_anchor_y: f64,
    rows: Rows<'_>,
) -> Option<(usize, f64)> {
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

    for (index, candidate) in rows.all.iter().enumerate() {
        if rows.used[index] || !is_valid_item_line(candidate, rows.total_line_y) {
            continue;
        }

        let distance = (candidate.line_y - selection_anchor_y).abs();
        if distance > MAX_ITEM_DISTANCE + SPATIAL_FLOAT_EPSILON {
            continue;
        }

        let candidate_has_trailing_price = line_has_trailing_price(&candidate.full_text);
        if candidate_has_trailing_price {
            if candidate.line_y > selection_anchor_y
                && nearest_unpriced_deposit_stub_below(rows.all, index, rows.used).is_some()
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
                    _ => nearest_deposit_stub_above_within_tolerance = Some((index, distance)),
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
    match (
        nearest_unpriced_above,
        nearest_unpriced_below,
        source_modifier && own_price != Some(false),
    ) {
        (Some(above), Some(_), true) => Some(above),
        (Some(above), Some(below), false) => {
            if above.1 <= below.1 {
                Some(above)
            } else {
                Some(below)
            }
        }
        (Some(above), None, _) => Some(above),
        // No regular item above: prefer a deposit stub within Y_TOLERANCE
        // over a non-deposit item below, so "2@$0.10" pairs with "DEPOSIT 1"
        // rather than the next real item below.
        (None, Some(below), _) => nearest_deposit_stub_above_within_tolerance.or(Some(below)),
        (None, None, _) => {
            nearest_priced_below_with_deposit_stub.or(nearest_deposit_stub_above_within_tolerance)
        }
    }
}

/// Whether a quantity expression should look below itself after all.
///
/// The default for a quantity row is to modify the item above it, but when the
/// only eligible row on its own y-band is *below*, that default has nothing to
/// bind to and the row below is the item. Narrow on purpose: it only fires when
/// there is an eligible row below and none above.
pub(super) fn quantity_row_prefers_below(selection_anchor_y: f64, rows: Rows<'_>) -> bool {
    let mut nearest_same_row_above = None;
    let mut nearest_same_row_below = None;

    for (index, candidate) in rows.all.iter().enumerate() {
        if rows.used[index] || !is_valid_item_line(candidate, rows.total_line_y) {
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

    nearest_same_row_below.is_some() && nearest_same_row_above.is_none()
}
