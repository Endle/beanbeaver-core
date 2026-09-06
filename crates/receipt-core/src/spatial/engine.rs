//! Emit spatial items in price order, keeping row claims in one place.
use super::pairing::*;
use super::patterns::*;
use super::rows::*;
use super::types::*;
use crate::money::Money;
use crate::ocr_document::OcrDocument;
pub fn extract_spatial_items(doc: &OcrDocument) -> SpatialExtractionOutcome {
    let mut items = Vec::new();
    let mut warnings = Vec::new();

    let (all_lines, price_candidates) = classify_rows(doc);
    let total_line_y = total_line_y(&all_lines);
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

        let Some(closest_line_index) = closest_row(price_y, &all_lines) else {
            continue;
        };
        let source_line = &all_lines[price_candidate.source_line_index];
        let closest_line = &all_lines[closest_line_index];
        let PriceContext {
            price_line_has_onsale,
            prefer_below,
        } = price_context(source_line, closest_line);

        let mut is_summary = price_is_summary(price_y, closest_line, &all_lines, total_line_y);

        let mut onsale_target_line_index = None;
        if !is_summary && price_line_has_onsale {
            match onsale_target(source_line.line_y, &all_lines) {
                Some(index) => onsale_target_line_index = Some(index),
                // An on-sale price with nothing it could be discounting is not
                // an item price at all — the marker is the receipt talking about
                // its own totals ("YOU SAVED"), so treat it as summary.
                None => is_summary = true,
            }
        }

        if is_summary {
            continue;
        }

        let mut found_item = false;
        let choice = select_target_line(
            &price_candidate,
            price_y,
            source_line,
            Rows {
                all: &all_lines,
                used: &used_line_indices,
                total_line_y,
            },
            prefer_below,
            price_line_has_onsale,
            onsale_target_line_index,
        );
        let LineChoice {
            line_index: chosen_line_index,
            distance: chosen_distance,
            prefer_below,
            suppress_fallback: suppress_fallback_for_ambiguous_code_only_source,
            source_repeats_previous_priced_item: source_line_repeats_previous_priced_item,
            shifted_to_deposit_stub,
        } = choice;
        let source_line_is_quantity_expression =
            looks_like_quantity_expression(&source_line.left_text);
        if let Some(index) = chosen_line_index {
            // A quantity expression or a prefer-below verdict means the row that
            // owns the price is a whole line away by design; anything else has to
            // be on the price's own row.
            let direct_match_tolerance = if source_line_is_quantity_expression || prefer_below {
                MAX_ITEM_DISTANCE + SPATIAL_FLOAT_EPSILON
            } else {
                Y_TOLERANCE + SPATIAL_FLOAT_EPSILON
            };
            if chosen_distance <= direct_match_tolerance
                && !re_mangled_reg_marker().is_match(all_lines[index].left_text.trim())
            {
                if let Some(description) = row_description(&all_lines[index]) {
                    used_line_indices[index] = true;
                    items.push(SpatialExtractedItem {
                        category_source: description.clone(),
                        quantity: 1,
                        description,
                        price: Money::from_scaled_4(price_candidate.price_scaled),
                    });
                    found_item = true;
                }
            }
        }

        if !found_item
            && !shifted_to_deposit_stub
            && !used_line_indices[price_candidate.source_line_index]
            && trailing_price_scaled(&source_line.full_text) == Some(price_candidate.price_scaled)
            && is_valid_item_line(source_line, total_line_y)
            && !looks_like_quantity_expression(&source_line.left_text)
        {
            if let Some(description) = row_description(source_line) {
                used_line_indices[price_candidate.source_line_index] = true;
                items.push(SpatialExtractedItem {
                    category_source: description.clone(),
                    quantity: 1,
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
            if let Some((index, description)) = nearest_describable_row_above(
                price_y,
                &all_lines,
                &used_line_indices,
                price_line_has_onsale,
            ) {
                used_line_indices[index] = true;
                items.push(SpatialExtractedItem {
                    category_source: description.clone(),
                    quantity: 1,
                    description,
                    price: Money::from_scaled_4(price_candidate.price_scaled),
                });
                found_item = true;
            }
        }
        if !found_item {
            warnings.push(missed_item_warning(
                &price_candidate,
                source_line,
                closest_line,
                items.len(),
            ));
        }
    }

    SpatialExtractionOutcome { items, warnings }
}
