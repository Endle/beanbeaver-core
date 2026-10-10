//! Ordered text extraction stages. Row claims remain owned by this loop.
use super::pairing::*;
use super::patterns::*;
use super::quantity::{
    looks_like_quantity_expression, qty_row_owns_trailing_total, recover_repeated_multi_buy_total,
};
use super::reconcile::*;
use super::rows::*;
use super::tokens::*;
use super::types::*;
use crate::money::Money;
use std::collections::HashSet;
pub fn extract_text_items(lines: &[String], summary_amounts: &HashSet<Money>) -> ExtractionOutcome {
    let mut deferred = Vec::new();
    let normalized_lines: Vec<String> = lines
        .iter()
        .map(|line| normalize_decimal_spacing(line))
        .map(|line| normalize_tax_code_ocr(&line))
        .map(|line| strip_trailing_sale_marker(&line))
        .collect();
    let (normalized_lines, price_corrections): (Vec<String>, Vec<Option<String>>) =
        normalized_lines
            .into_iter()
            .map(|line| match recover_repeated_multi_buy_total(&line) {
                Some((corrected, evidence)) => (corrected, Some(evidence)),
                None => (line, None),
            })
            .unzip();
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
    let price_drift = count_price_drift_evidence(&normalized_lines).establishes_drift();

    // The department banner in force at each row. Items are stamped with the
    // department of the row whose processing emitted them; the loop has many
    // `continue`s, so that happens on entry to the next row rather than at the
    // bottom of this one.
    let departments = crate::department::in_effect(normalized_lines.iter().map(String::as_str));
    let mut stamped = 0;

    for (i, line) in normalized_lines.iter().enumerate() {
        if i > 0 {
            stamp_department(&mut deferred[stamped..], departments[i - 1]);
            stamped = deferred.len();
        }
        if total_line_idx.is_some_and(|total_idx| i > total_idx) {
            break;
        }
        if re_skip_patterns().is_match(line) {
            continue;
        }
        if line.len() < 3 || re_digits_only().is_match(line) {
            continue;
        }

        let deferred_start = deferred.len();
        let is_qty_line = looks_like_quantity_expression(line);
        let has_trailing_total =
            re_trailing_total_presence().is_match(line) || qty_row_owns_trailing_total(line);
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

            // Short labels may also be OCR debris under an unpriced product
            // name. Preserve an existing description pairing first; use the
            // short inline label only when no better description was found.
            let inline_description = plan.describes_itself();
            if inline_description && plan.desc_part.len() > 2 {
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
                    None if inline_description => {
                        deferred.push(DeferredTextOutcome::Item(inline_item(&plan, price_cents)));
                        used_text_lines[i] = true;
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
        if deferred[deferred_start..]
            .iter()
            .any(|outcome| matches!(outcome, DeferredTextOutcome::Item(_)))
        {
            if let Some(evidence) = &price_corrections[i] {
                deferred.push(DeferredTextOutcome::Warning(
                    crate::common::ReceiptWarningKind::PriceAutoCorrected,
                    evidence.clone(),
                ));
            }
        }
    }

    // The rows after the last stamp were all emitted by the final row processed:
    // the last row, or the one the loop broke on (which emits nothing).
    if let Some(last) = departments.last() {
        stamp_department(&mut deferred[stamped..], *last);
    }

    let (mut items, mut warnings) = resolve_deferred(deferred, summary_amounts);

    if let Some(cap_base) = total_cap_cents {
        items = drop_prices_above_cap(items, &mut warnings, cap_base);
    }

    ExtractionOutcome { items, warnings }
}

fn stamp_department(outcomes: &mut [DeferredTextOutcome], department: Option<&'static str>) {
    for outcome in outcomes {
        if let DeferredTextOutcome::Item(item) = outcome {
            item.department = department;
        }
    }
}
