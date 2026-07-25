//! OCR glyph-confusion costs and the fuzzy string distance built on them.
//!
//! Receipt OCR does not fail uniformly: some substitutions are near-certain
//! artifacts of the glyph set (`0` for `O`), others are plausible only when a
//! stroke fades or blots (`M` for `H`), and most are simply wrong. Charging every
//! substitution the same price throws that information away, so this module
//! grades it.
//!
//! Costs are **discrete tiers, deliberately not tuned floats.** The tiers encode
//! a visual argument a reviewer can check by looking at the glyphs; a fitted
//! `0.37` would encode noise from whatever corpus produced it. Do not replace
//! them with per-pair regressed values without a corpus large enough to have real
//! per-pair counts.
//!
//! Everything here assumes input is already uppercased and ASCII — callers
//! normalize first (see `merchant_match::normalize`), so lowercase pairs are
//! intentionally absent from the table.

/// Same printed glyph, different codepoint. OCR choosing the wrong one is barely
/// an error at all, so it costs almost nothing.
const COST_SAME_GLYPH: f64 = 0.1;

/// Distinct glyphs sharing a stroke skeleton: a faded, blotted, or bridged
/// stroke turns one into the other. `M`/`H` is the motivating case — a thermal
/// `M` whose middle diagonal drops out reads as `H`.
const COST_SHARED_SKELETON: f64 = 0.3;

/// Confusable only under heavy smearing. Plausible, but weak evidence, so it
/// costs most of a full edit.
const COST_SMEAR_ONLY: f64 = 0.6;

/// No visual relationship — a whole edit. Also the ceiling: no substitution ever
/// costs more than an insert or a delete.
const COST_UNRELATED: f64 = 1.0;

/// Cost in `[0, 1]` of OCR reading `a` where `b` was printed (or vice versa —
/// the table is symmetric).
///
/// Real OCR confusion is mildly directional: `0`→`O` and `O`→`0` differ in
/// likelihood depending on whether the surrounding field is numeric. Modelling
/// that is not worth the table doubling, so this stays order-independent.
pub fn confusion_cost(a: char, b: char) -> f64 {
    if a == b {
        return 0.0;
    }
    // ASCII digits sort before uppercase letters, so `lo`/`hi` gives each pair
    // exactly one spelling in the match below.
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    match (lo, hi) {
        ('0', 'O')
        | ('1', 'I')
        | ('1', 'L')
        | ('I', 'L')
        | ('5', 'S')
        | ('8', 'B')
        | ('2', 'Z')
        | ('6', 'G') => COST_SAME_GLYPH,

        ('0', 'D')
        | ('0', 'Q')
        | ('D', 'O')
        | ('O', 'Q')
        | ('C', 'G')
        | ('E', 'F')
        | ('H', 'M')
        | ('H', 'N')
        | ('M', 'N')
        | ('M', 'W')
        | ('U', 'V') => COST_SHARED_SKELETON,

        ('P', 'R') | ('K', 'X') => COST_SMEAR_ONLY,

        _ => COST_UNRELATED,
    }
}

/// Levenshtein distance with [`confusion_cost`] pricing substitutions; inserts
/// and deletes cost a full 1.0.
///
/// Because a confusable swap is cheap, OCR's systematic glyph errors rank far
/// ahead of coincidental edits of the same count — which is the whole point of
/// grading them.
pub fn weighted_distance(a: &[char], b: &[char]) -> f64 {
    if a.is_empty() {
        return b.len() as f64;
    }
    if b.is_empty() {
        return a.len() as f64;
    }
    let mut prev: Vec<f64> = (0..=b.len()).map(|j| j as f64).collect();
    let mut curr: Vec<f64> = vec![0.0; b.len() + 1];
    for i in 1..=a.len() {
        curr[0] = i as f64;
        for j in 1..=b.len() {
            let sub = confusion_cost(a[i - 1], b[j - 1]);
            curr[j] = (prev[j] + 1.0)
                .min(curr[j - 1] + 1.0)
                .min(prev[j - 1] + sub);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Normalized similarity in `[0, 1]`: `1 - distance / longer_length`. Dividing by
/// the longer input is what keeps a given score comparable across lengths, so
/// callers can use one absolute threshold.
pub fn similarity(a: &str, b: &str) -> f64 {
    let ca: Vec<char> = a.chars().collect();
    let cb: Vec<char> = b.chars().collect();
    let longer = ca.len().max(cb.len());
    if longer == 0 {
        return 0.0;
    }
    1.0 - weighted_distance(&ca, &cb) / longer as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dist(a: &str, b: &str) -> f64 {
        weighted_distance(
            &a.chars().collect::<Vec<_>>(),
            &b.chars().collect::<Vec<_>>(),
        )
    }

    #[test]
    fn identical_chars_are_free_and_unrelated_chars_cost_a_full_edit() {
        assert_eq!(confusion_cost('A', 'A'), 0.0);
        assert_eq!(confusion_cost('A', 'Z'), COST_UNRELATED);
        assert_eq!(confusion_cost('M', 'Z'), COST_UNRELATED);
    }

    #[test]
    fn cost_is_symmetric() {
        for (a, b) in [('0', 'O'), ('H', 'M'), ('K', 'X'), ('A', 'Q')] {
            assert_eq!(confusion_cost(a, b), confusion_cost(b, a), "{a} vs {b}");
        }
    }

    #[test]
    fn no_substitution_ever_exceeds_an_insert_or_delete() {
        // The ceiling is what lets `similarity` keep one meaningful threshold:
        // the metric degrades to plain Levenshtein for unrelated pairs.
        for a in ('A'..='Z').chain('0'..='9') {
            for b in ('A'..='Z').chain('0'..='9') {
                assert!(confusion_cost(a, b) <= COST_UNRELATED, "{a} vs {b}");
            }
        }
    }

    #[test]
    fn tiers_are_strictly_ordered_by_visual_similarity() {
        let same_glyph = confusion_cost('0', 'O');
        let skeleton = confusion_cost('H', 'M');
        let smear = confusion_cost('K', 'X');
        assert!(0.0 < same_glyph);
        assert!(same_glyph < skeleton);
        assert!(skeleton < smear);
        assert!(smear < COST_UNRELATED);
    }

    #[test]
    fn digit_letter_swaps_cost_far_less_than_a_real_letter_error() {
        // "C0STC0" is what OCR does to "COSTCO"; "CQSTCA" is not.
        assert!(dist("C0STC0", "COSTCO") < dist("CASTCA", "COSTCO"));
        assert_eq!(dist("C0STC0", "COSTCO"), 2.0 * COST_SAME_GLYPH);
    }

    #[test]
    fn several_free_swaps_stay_cheaper_than_one_genuine_error() {
        // The case a binary allow-k-substitutions rule gets backwards: two
        // near-certain glyph swaps are better evidence than one wrong letter.
        // `k = 1` would reject the former and accept the latter.
        let two_trivial = dist("10B", "IOB"); // 1/I and 0/O
        let one_wrong = dist("AOB", "IOB"); // A/I, unrelated
        assert_eq!(two_trivial, 2.0 * COST_SAME_GLYPH);
        assert_eq!(one_wrong, COST_UNRELATED);
        assert!(two_trivial < one_wrong);
    }

    #[test]
    fn watermelon_ocr_error_is_cheap() {
        // The founding case: a No Frills receipt printed "WMELON RED SDLS" and
        // OCR returned "WHELON" — an M read as H, one shared-skeleton swap.
        assert_eq!(dist("WHELON", "WMELON"), COST_SHARED_SKELETON);
        assert!(similarity("WHELON", "WMELON") > 0.9);
    }

    #[test]
    fn similarity_is_bounded_and_length_normalized() {
        assert_eq!(similarity("", ""), 0.0);
        assert_eq!(similarity("COSTCO", "COSTCO"), 1.0);
        assert!(similarity("COSTCO", "ZZZZZZ") <= 0.0);
        // Same single unrelated substitution costs proportionally less on a
        // longer string, which is what makes one absolute threshold workable.
        assert!(similarity("COSTCOXX", "COSTCOXZ") > similarity("CAT", "CAZ"));
    }

    #[test]
    fn insert_and_delete_still_cost_a_full_edit() {
        // The documented HIGH_SIMILARITY calibration in `merchant_match` rests on
        // a dropped character ("OSTCO" vs "COSTCO"), so grading substitutions
        // must not have made deletions cheaper.
        assert_eq!(dist("OSTCO", "COSTCO"), 1.0);
        assert!((similarity("OSTCO", "COSTCO") - 0.8333).abs() < 0.001);
    }
}
