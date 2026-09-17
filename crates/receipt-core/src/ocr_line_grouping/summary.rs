//! Recover a complete tilted summary block using its two independent sums.
use super::{
    boxes_overlap_y, is_standalone_money, Detection, Money, OnceLock, Regex, PAIR_OVERLAP_GATE,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Label {
    Subtotal,
    Tax,
    Total,
    Card,
}

/// Exact-match only, and deliberately narrower than the sibling
/// [`super::is_summary_anchor_label`]: no `T[OCQDG0]TAL` confusion set, no
/// trailing colon, and `CREDIT CARD` rather than the card brands.
///
/// The cost is reach, not safety — the block is all-or-nothing, so one label
/// this misses declines the whole correction and pairing falls back to the
/// path it took before. Widen it against a corpus diff, one form at a time; a
/// looser vocabulary here reserves detections *ahead of* first-fit pairing,
/// which is the expensive direction to be wrong in.
fn label(text: &str) -> Option<Label> {
    let text = text.trim().to_ascii_uppercase();
    match text.as_str() {
        "SUB TOTAL" | "SUBTOTAL" => Some(Label::Subtotal),
        "TOTAL AFTER TAX" => Some(Label::Total),
        "CREDIT CARD" => Some(Label::Card),
        _ => {
            static TAX: OnceLock<Regex> = OnceLock::new();
            TAX.get_or_init(|| Regex::new(r"^(?:HST|GST|PST|TAX)(?:\s*\d+(?:\.\d+)?%)?$").unwrap())
                .is_match(&text)
                .then_some(Label::Tax)
        }
    }
}

/// The summary's amount column can sit more than a row below its labels.
/// Ordinary overlap pairing then gives SUBTOTAL to HST and a zero tax to
/// TOTAL. Only reserve a complete, unambiguous block: subtotal, taxes, total,
/// one card payment; exactly one printed amount per label in column order;
/// subtotal + taxes = total = card payment. No amount is synthesized or edited.
pub(super) fn reconciled_groups(dets: &[Detection]) -> Vec<Vec<usize>> {
    let mut order: Vec<_> = (0..dets.len()).collect();
    order.sort_by(|&a, &b| dets[a].center_y.total_cmp(&dets[b].center_y));
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (start, &first) in order.iter().enumerate() {
        if label(&dets[first].text) != Some(Label::Subtotal) {
            continue;
        }
        let mut labels = vec![first];
        let mut saw_total = false;
        for &index in &order[start + 1..] {
            if is_standalone_money(&dets[index].text) {
                continue;
            }
            match label(&dets[index].text) {
                Some(Label::Tax) if !saw_total => labels.push(index),
                Some(Label::Total) if !saw_total && labels.len() > 1 => {
                    labels.push(index);
                    saw_total = true;
                }
                Some(Label::Card) if saw_total => {
                    labels.push(index);
                    break;
                }
                _ => break,
            }
        }
        let last = *labels.last().unwrap();
        if label(&dets[last].text) != Some(Label::Card) {
            continue;
        }
        let max_height = labels
            .iter()
            .map(|&i| dets[i].y_max - dets[i].y_min)
            .fold(0.0, f64::max);
        // Seeded at -inf, not 0.0: de-padding subtracts the OCR pad from every
        // point, so a label box that starts inside the pad has a negative
        // `min_x` and a 0.0 seed would win the max — admitting amounts printed
        // to the *left* of the labels.
        let left_edge = labels
            .iter()
            .map(|&i| dets[i].min_x)
            .fold(f64::NEG_INFINITY, f64::max);
        // The window runs downward from the first label because that is the
        // drift this exists to undo: the amount column lags its labels. A
        // column that leans *up* far enough to put an amount above
        // `first.y_min` is not recovered here — see the note on
        // `PAIR_OVERLAP_GATE`, which compensates for the up-lean case.
        let amounts: Vec<_> = order
            .iter()
            .copied()
            .filter(|&i| {
                dets[i].min_x > left_edge
                    && dets[i].center_y >= dets[first].y_min
                    && dets[i].center_y <= dets[last].y_max + max_height
                    && is_standalone_money(&dets[i].text)
            })
            .collect();
        if amounts.len() != labels.len() {
            continue;
        }
        // Keep the correction local. A label and its amount must be within
        // their combined box heights, even when their spans do not overlap.
        if labels.iter().zip(&amounts).any(|(&l, &a)| {
            (dets[l].center_y - dets[a].center_y).abs()
                > (dets[l].y_max - dets[l].y_min) + (dets[a].y_max - dets[a].y_min)
        }) {
            continue;
        }
        let values: Vec<_> = amounts
            .iter()
            .map(|&i| {
                Money::parse_strict(dets[i].text.trim().trim_start_matches('$').trim())
                    .unwrap()
                    .cents()
            })
            .collect();
        let total_index = values.len() - 2;
        let sum = values[..total_index]
            .iter()
            .try_fold(0_i64, |acc, &v| acc.checked_add(v));
        if values.iter().any(|&v| v < 0)
            || values[0] == 0
            || sum != Some(values[total_index])
            || values[total_index] != values[total_index + 1]
        {
            continue;
        }
        // Already aligned blocks need no special handling.
        if labels
            .iter()
            .zip(&amounts)
            .all(|(&l, &a)| boxes_overlap_y(&dets[l], &dets[a], PAIR_OVERLAP_GATE))
        {
            continue;
        }
        let candidate: Vec<_> = labels
            .into_iter()
            .zip(amounts)
            .map(|(l, a)| vec![l, a])
            .collect();
        if candidate
            .iter()
            .flatten()
            .any(|i| groups.iter().any(|g| g.contains(i)))
        {
            continue;
        }
        groups.extend(candidate);
    }
    groups
}
