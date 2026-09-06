//! Text extraction: reconcile.
use super::patterns::*;
use super::quantity::*;
use super::rows::*;
use super::types::*;
use crate::common::ReceiptWarningKind;
use crate::money::Money;
use std::collections::{HashMap, HashSet};
pub(super) fn maybe_push_warning(
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
pub(super) fn truncated_context(line: &str) -> String {
    // Truncate to 80 *characters* (matching Python's `[:80]`); a byte-index
    // `truncate(80)` panics when byte 80 lands inside a multibyte char (e.g.
    // CJK text on Asian-grocery receipts).
    let trimmed = line.trim();
    match trimmed.char_indices().nth(80) {
        Some((byte_idx, _)) => trimmed[..byte_idx].to_string(),
        None => trimmed.to_string(),
    }
}
pub(super) fn extract_trailing_noisy_price(line: &str) -> Option<(String, String, i64, usize)> {
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
pub(super) fn build_malformed_price_candidate(
    line: &str,
) -> Option<MalformedTrailingPriceCandidate> {
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
pub(super) fn levenshtein_distance(left: &str, right: &str) -> usize {
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
pub(super) fn malformed_candidate_price_options(
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
pub(super) fn reconcile_malformed_price_candidates(
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
pub(super) fn resolve_deferred(
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
pub(super) fn drop_prices_above_cap(
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

/// A `N/for` multi-buy row whose total OCR mangled into a token carrying
/// letters ("3/for 5.0O"). The amount is unrecoverable — there is no second
/// reading of it anywhere on the receipt — so the only honest output is to say
/// an item may have been missed here.
///
/// Two arms of the loop reach this, and they used to carry a copy each: a
/// `/for` row is a quantity expression whether or not its mangled tail happened
/// to parse as a trailing price, so both the qty-row arm and the no-price tail
/// need the same answer.
pub(super) fn multi_buy_tail_warning(line: &str) -> Option<DeferredTextOutcome> {
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
pub(super) fn unpriced_line_outcome(line: &str) -> Option<DeferredTextOutcome> {
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
